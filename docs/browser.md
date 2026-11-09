# Session browser

The reader shows recent session titles first, with a source filter and
title/directory/source matching across the archive. It loads 50 sessions initially;
older sessions load only on request. ⌘K focuses search. Selecting a session opens its conversation;
the URL preserves both source and session identity. It does not send prompts or
change the archive. Reasoning, tools, system context and compaction summaries are
expandable. Attachments are labeled, not fetched from external storage. Archived
HTML is inert, remote images are not loaded, and Markdown is sanitized.

## Run locally

Run `bun install --frozen-lockfile`, `bun run web:build`, and
`cargo build --locked --package spool-server`. Point `DATABASE_URL` at an existing
Spool database and run `target/debug/spool-server browse`. It serves the built
assets from `web/dist` at `http://127.0.0.1:8090` by default. `--assets` selects
another build directory. It never migrates the database.

For another machine on your tailnet, use `browse --bind <your-tailscale-ip>` and
open the literal IP URL. The listener rejects wildcard, LAN and public binds.
MagicDNS names, custom Host headers and reverse proxies are not supported; the
Host restriction prevents DNS rebinding. HTTP over the tailnet is encrypted by
Tailscale, but the browser does not treat it as an HTTPS secure context.

**Access to this listener means read access to every source.** It has no login:
loopback or your tailnet's access policy is the boundary. Do not publish it through
a tunnel, Funnel, or public proxy. Source tokens still authorize ingestion only.
The public `serve` mode does not mount the reader or its API. Adding production
read access through the public internet would require a separate authentication
design. The [private production reader](reader-deployment.md) instead uses a
separate tailnet-only service with restricted database credentials.

The browser uses a separate two-connection pool with PostgreSQL's
`default_transaction_read_only=on`; its routes execute only reads. This setting
prevents accidental writes, not misuse of an administrative database credential.
A dedicated SELECT-only database role is appropriate for a persistent installation.
Responses prohibit caching, framing and cross-origin API access.

A local copy of the production archive is useful for a preview without changing
the cloud deployment. Treat that copy as private data, not a synthetic fixture;
it is a point-in-time snapshot and does not follow new uploads automatically.
Production token hashes are not needed in a preview copy: populate source labels
with inert hashes instead. Never expose PostgreSQL outside loopback for this.

## Read API

Both endpoints exist only in `browse` mode. Metadata and message documents remain
opaque JSON, including unknown fields and escaped NUL. Responses contain no
source credential fields. Purged sessions are omitted/return 404; revoked sources'
already archived sessions remain readable.

`GET /healthz` is a separate private readiness probe: a bounded database read,
204 with no body on success, and 503 when unavailable. It does not list sessions.

- `GET /api/sessions?limit=50&cursor=…&q=…&source={source_uuid}` returns
  `{sessions: [{source_id, source_label, session_id, revision, info, received_at}], sources: [{source_id, source_label}], next_cursor}`.
  The limit is 1–500. Inventory is ordered by exported update time in milliseconds,
  falling back to receipt time for missing/invalid dates; source UUID/session ID
  break ties descending. The optional `q` is a case-insensitive literal substring
  of title, directory or source label; `source` restricts results to one source.
  Source choices cover the archive, not just the loaded/filtered page. No message
  bodies are fetched until a session is selected. List pagination is eventually
  consistent; restart from the first page when changing filters.

  The reader streams metadata and retains page-sized candidates for ordering and
  filtering. PostgreSQL JSON extraction rejects escaped NUL even in unrelated
  fields, so metadata interpretation stays in Rust rather than SQL. This avoids
  downloading the whole inventory into the browser, but still scans source
  metadata per request; there is no indexed search projection in this version.
- `GET /api/sources/{source_uuid}/sessions/{session_id}?limit=100&cursor=…` returns
  `{source_id, source_label, session_id, revision, info, received_at, messages, next_cursor}`.
  The limit is 1–200. Messages follow archived order. Each page reads metadata and
  messages in one repeatable-read transaction. Cursors bind source, session,
  revision and position; if an upload changes the revision between pages, the API
  returns 409 and the reader asks for a reload rather than mixing snapshots.

Revisions are decimal strings; `next_cursor` is opaque or null. Malformed paths,
limits and cursors return 400, missing sessions return 404, and database failures
return generic 503 errors. Error bodies use `{error: {code: "…"}}`.

## Validation

`bun run web:validate` builds the frontend and native Rust reader, starts the
isolated container stack, ingests synthetic sessions over HTTP, then runs
Playwright against the built app. It uses installed Google Chrome in a disposable
browser profile, not personal tabs or cookies. There are no model calls or real
conversations. Containers and volumes are removed afterward; failed runs retain
logs under `.local/web-validate-*`. Screenshots and traces go to `.amp/in/artifacts/`.

The checks exercise source collisions, filtering, history/deep links, Markdown,
tools/reasoning/compaction, ordered pagination, revision conflicts, inert hostile
content, recoverable failures and mobile navigation. Rust tests independently
check the SQL snapshots, raw JSON, read-only connections and HTTP access guards.
