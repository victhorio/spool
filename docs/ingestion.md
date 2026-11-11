# Ingestion

Spool stores the latest successfully captured settled OpenCode export of each
session. It is eventually consistent, not an edit history, an atomic snapshot of
OpenCode, or a replica of its database. Unfinished output and files outside the
export are not archived. Deleting a session locally keeps its archive; a
committed undo replaces its contents once captured.

The plugin owns enumeration, export, content hashing, retries and checkpoints.
The server authenticates, validates the upload and writes it atomically. Raw
OpenCode objects stay opaque to the server except for IDs, and are stored as
PostgreSQL `json`, because `jsonb` rejects valid escaped NUL strings; don't cast
archived documents to `jsonb`. For search, the plugin supplies text projections
alongside the raw messages; see [retrieval](retrieval.md).

## Sources and tokens

A source is one independent OpenCode store, not a hostname, Location or process.
Give each store its own token, even on the same computer, and keep it across
restarts. Never copy a token into a second store: revisions fence stale uploads
from one store, but cannot decide which of two diverging stores should win.
Imported session IDs in different sources remain separate archives.

A token is `spool_v1.<source-uuid>.<secret>`; only the secret's SHA-256 is
stored. Any valid token may read the whole archive, but cannot administer it.
Operator commands (`spool-server source add|rotate|revoke`, `session purge`,
`artifact purge`) use direct database access, not a public endpoint.

A source may have an alias such as `work-m5`, which names it in session
references. Aliases are globally unique and permanent: they cannot be renamed,
cleared or reused, even after revocation. The plugin's `sourceAlias` option
registers it once per collector lifetime.

### Device login

A machine without a token gets one through device login, modelled on RFC 8628;
`bun run install-plugin` runs it when the token file is missing. The machine
requests a code for an alias, the owner types the code at `/device` in the
reader and approves it, and the machine collects a new token exactly once.
The page never takes a code from a link, so a link someone else sends cannot
pose as the owner's own request. Only a hash of the device code is stored, and
the token exists only in the collection response.

## Snapshots

The plugin uploads a session's whole message order, with the bodies of new or
changed messages, against the revision it last saw (`base_revision`, null to
create). Every acceptance advances the revision, even when nothing changed, so a
newer confirmation fences an older observation. On 409 `revision_conflict` the
plugin discards its capture and digest baseline, learns the current revision and
exports again; it never relabels an old payload with a new revision. Lost
acknowledgements recover the same way, so no upload ledger or durable queue is
needed.

A session becomes searchable only once every current message has a projection.
An upload without projections is still accepted, but makes the session
unsearchable until a complete one arrives. Repository identity is kept unless an
upload supplies a new nonempty one, so a collector that loses its checkpoint
cannot erase a known association.

The archived part of an upload is limited to 16 MiB and the whole upload,
including projections, to 64 MiB. These bound decoding and database memory, not
just wire bytes. A larger session stays unsynced rather than truncated.

## Capture

Capture is event-driven; there is no periodic audit. Startup, attaching a
Location and reconnecting compare local sessions with the archive's inventory
and queue only missing, changed or mismatched ones. A capture exports and hashes
the session but uploads only when its info, message order, messages or
repository changed. Captures run one at a time, and only while some Location has
loaded the plugin.

`/spool-repair` in OpenCode queues a full audit of the source's local sessions,
re-exporting them and rechecking Git identity, for edits that produced no event.
It cannot recover a session already deleted locally or resurrect a purged one.

Logs carry IDs, counts and fixed codes, never transcripts, credentials or raw
error text.

## Purge

`spool-server session purge <source-uuid> <session-id>` deletes a session's
content and the artifacts it created, leaving a tombstone that rejects future
uploads with 410. Backups and WAL keep old content until they expire.
