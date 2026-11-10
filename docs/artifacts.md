# Artifacts

An artifact is a document an agent publishes to Spool for a person to read: a
plan, report, chart or image. The agent gets back a stable link such as
`https://spool.example.com/artifacts/art_k3x9m2p7q4wd`, which opens it in the
reader. Agents write and revise artifacts; people only read them, and there is no
editing in the browser. Artifacts are separate from archived sessions: they have
their own tools, list and search.

## Content

Each artifact is one document of one type, at most 32 MiB:

| Type | Media type | Shown as |
|---|---|---|
| Markdown | `text/markdown` | Rendered, sanitized Markdown |
| HTML | `text/html` | The page |
| Text | `text/plain` | Preformatted text |
| Image | `image/png`, `image/jpeg`, `image/gif`, `image/webp`, `image/svg+xml` | The image |

Markdown, HTML, text and SVG are text types: UTF-8 without NUL, stored as text,
so agents can read, search and edit them. Raster images must start with their
format's signature. An image is its own artifact, so Markdown and HTML artifacts
can show it by URL, such as `![chart](https://spool.example.com/artifacts/art_…)`
in Markdown or `<img src="https://spool.example.com/artifacts/art_…/content">` in
HTML. Both may also load any remote resource.

The server keeps only the current version. Every change increments the
artifact's `revision`, and a change names the revision it was based on, so a
concurrent change fails with 409 `revision_conflict` instead of being lost. The
artifacts first created by one source may total at most 1 GiB.

## Trust

Artifacts come from the owner's own agents, which already act with the owner's
access, so the reader serves them as ordinary pages on its own origin.
`/artifacts/{id}/content` returns the raw content; its only policy is
`frame-ancestors 'self'`, so other sites cannot frame it.

`spool_create_artifact` and `spool_update_artifact` read `path` with the
OpenCode process's own access, outside OpenCode's file permissions, and every
source token can read the result.

## Agent tools

The plugin registers these OpenCode tools, in the `spool` namespace, and the
bundled `publishing-artifacts` skill explains when and how to use them:

- `spool_create_artifact` publishes inline `content` with a `type`
  (`markdown`, `html` or `text`), or a local file `path`, whose extension sets
  the type. It records the calling session and returns the link.
- `spool_update_artifact` replaces content and/or the title.
- `spool_edit_artifact` applies exact find-and-replace edits to a text artifact
  on the server, so a small change does not resend the whole document.
- `spool_find_artifacts` lists artifacts by recent update, optionally matching
  literal terms in the title and text content.
- `spool_read_artifact` returns metadata and, for text types, content in chunks
  of up to 8 KiB with a continuation offset.

Agents may publish without being asked when the result is meant to be read, such
as a plan, report or findings, and should give the person the link. Any source
may update any artifact. There is no agent delete.

## Machine API

All routes need a source token, like the rest of `/v1`. Artifact objects are
`{id, title, type, revision, size, source, source_id, source_label, session,
session_id, created_at, updated_at}`. `type` is the media type and `size` is in
bytes. `revision` is a decimal string. `source` is the creating source's alias,
or its UUID if it has none. `session` is the creating session as a
[reference](retrieval.md#session-references) for the retrieval tools and reader
links (the native ID, qualified only when another source has the same ID), and
`session_id` its native ID; both are null when no session was given. The plugin
passes agents only `{id, title, type, revision, size, source, session,
created_at, updated_at}` plus the artifact's reader `url` and its raw `content_url`.

- `POST /v1/artifacts?title=…&session=…` creates an artifact from the raw
  request body, whose `Content-Type` is the media type (a `charset` parameter is
  allowed for text). `session` is optional. Returns 201 `{artifact}`.
- `PUT /v1/artifacts/{id}?revision=…&title=…` replaces the content, and the type
  if `Content-Type` differs; `title` is optional. Returns `{artifact}`.
- `PATCH /v1/artifacts/{id}` with a JSON body `{revision, title?, edits?}`, up
  to 32 MiB, renames the
  artifact and/or edits a text artifact. `edits` is a list of `{old, new}`;
  each `old` must occur exactly once in the content as it stands after the
  previous edits. Edits apply all or nothing. A failing edit returns 422
  `edit_not_found` or `edit_not_unique` with its zero-based `edit` index.
- `GET /v1/artifacts?query=…&limit=…&cursor=…` returns
  `{artifacts, next_cursor}`, most recently updated first. `query` is up to 16
  whitespace-separated literal terms, all of which must occur in the title or
  text content, case-insensitively. `limit` is 1–50 (default 20).
- `GET /v1/artifacts/{id}?offset=…` returns `{artifact, text, offset, next_offset}`.
  For text types, `text` holds up to 8 KiB starting at character `offset`
  (default 0), and `next_offset` is where the next chunk starts, or null at the
  end. For images, `text` and `next_offset` are null.

Titles are 1–256 bytes after trimming, without control characters. Bodies over
1 MiB, or of unknown length, share two upload slots; when both are taken the
request fails with 503 `busy`. Errors: 400 `malformed_request` or
`request_timeout` (a body not received within 120 seconds); 401 `unauthorized`;
404 `not_found`; 409 `revision_conflict` with `current_revision`; 413
`payload_too_large` or `artifact_quota_exceeded`; 415 `unsupported_content_type`
or `unsupported_content_encoding`; 422 `invalid_content` when the bytes do not
match the type, or `not_text` when editing an image; 503 `busy` or `try_again`.
The plugin adds `file_unreadable` for a `path` it cannot read, and reports any
other failure as `archive_unavailable`.

The quota counts an artifact against the source that created it, whoever
updates it later.

## Reader

`/artifacts` lists artifacts, newest first, with the same search as agents use,
and `/artifacts/{id}` shows one, with its creating source and a link to its
session. The reader API is `GET /api/artifacts?q=…&limit=…&cursor=…` (limit
1–100; a cursor is used alone) and `GET /api/artifacts/{id}`, with the same
objects and Access checks as the rest of `/api/*`. The content itself is only at
`/artifacts/{id}/content`, which serves Markdown as plain text for the reader to
render. Markdown and text over 2 MiB are not rendered; the page links to the raw
content instead.

## Purge

`spool-server artifact purge <id>` deletes an artifact. `spool-server session
purge` also deletes the artifacts that session created. Both need direct
database access.
