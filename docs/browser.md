# Session browser

The reader shows recent session titles first, with a source filter and
title/directory/source matching across the archive. It loads 50 sessions initially;
older sessions load only on request. ⌘K focuses search. Selecting a session opens its conversation;
the URL preserves both source and session identity. It does not send prompts or
change the archive. Reasoning, tools, system context and compaction summaries are
expandable. Attachments are labeled, not fetched from external storage. Archived
HTML is inert, remote images are not loaded, and Markdown is sanitized.

## Access

The reader is part of the public `serve` service, at `https://spool.example.com`
in production. Cloudflare Access protects every route except `/v1/*`, which is for
machines with source tokens. The production Access application allows one GitHub
identity; see [public deployment](public-deployment.md).

Railway's origin can be reached without passing through Cloudflare, so the
server also verifies Access itself. Every reader request needs a
`Cf-Access-Jwt-Assertion` header: an RS256 assertion signed by a key from the
team's `/cdn-cgi/access/certs`, with the configured issuer and AUD tag, unexpired.
Requests without a valid assertion receive 403 `forbidden`. If the signing keys
cannot be fetched, the reader returns 503 `access_unavailable`. Keys are fetched
on first use and refreshed, at most every 30 seconds, when an assertion names an
unknown key.

`serve` enables the reader only when both `ACCESS_ISSUER` (the team origin, such
as `https://example.cloudflareaccess.com`) and `ACCESS_AUD` (the application's AUD
tag) are set; with neither, it serves only `/healthz` and `/v1/*`. `ASSETS_DIR`
selects the built frontend (default `web/dist`; the image uses `/opt/spool/web`).
Startup fails if only one Access variable is set or the assets are missing.

Reads use a separate two-connection pool with PostgreSQL's
`default_transaction_read_only=on`. This prevents accidental writes; it is not a
privilege boundary. Responses prohibit caching, framing and cross-origin API
access. `/api/*` additionally rejects cross-site Fetch Metadata and any Origin
that does not match the request's Host. When an Access session expires, API
requests are redirected to Cloudflare's login; the page reloads to follow it.

A local copy of the production archive is useful for a preview without changing
the cloud deployment. Treat that copy as private data, not a synthetic fixture;
it is a point-in-time snapshot and does not follow new uploads automatically.
Never expose PostgreSQL outside loopback for this.

## Read API

Metadata and message documents remain opaque JSON, including unknown fields and
escaped NUL. Responses contain no source credential fields. Purged sessions are
omitted/return 404; revoked sources' already archived sessions remain readable.

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

`bun run web:validate` builds the frontend and the Rust server, starts the
isolated container stack, ingests synthetic sessions over HTTP, then runs
Playwright against the built app. A synthetic Access team serves an in-memory
signing key, and the browser sends its assertion. It uses installed Google Chrome
in a disposable browser profile, not personal tabs or cookies. There are no model
calls or real conversations. Containers and volumes are removed afterward; failed
runs retain logs under `.local/web-validate-*`. Screenshots and traces go to
`.amp/in/artifacts/`.

The checks exercise source collisions, filtering, history/deep links, Markdown,
tools/reasoning/compaction, ordered pagination, revision conflicts, inert hostile
content, recoverable failures, expired Access sessions and mobile navigation. Rust
tests independently check Access assertions (signature, issuer, audience, expiry,
algorithm and key ID), the SQL snapshots, raw JSON, read-only connections and HTTP
access guards.
