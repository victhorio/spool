# Artifacts

An artifact is a document an agent publishes to Spool for a person to read: a
plan, report, chart or image. The agent gets back a stable link, such as
`https://spool.example.com/artifacts/art_k3x9m2p7q4wd`, that opens it in the
reader. Agents write and revise artifacts through the plugin's `spool_*_artifact`
tools; people only read them. The bundled `publishing-artifacts` skill tells
agents when to publish. Artifacts are separate from archived sessions, with
their own list and search.

Each artifact is one Markdown, HTML, plain text, SVG or raster image document of
at most 32 MiB. Text types are stored as text so agents can search and edit them
in place, and are stored uncompressed so a chunked read fetches only the slices
it needs. An image is its own artifact, which Markdown and HTML artifacts embed
by its content URL.

Only the current version is kept. Every change names the revision it is based
on, so a concurrent change fails with `revision_conflict` instead of being lost.
Any source may update any artifact, and there is no agent delete. The artifacts
a source created count against its 1 GiB quota, whoever updates them later.

## Trust

Artifacts come from the owner's own agents, which already act with the owner's
access, so the reader serves them as ordinary pages on its own origin, free to
load remote resources. Their raw content at `/artifacts/{id}/content` only
forbids framing by other sites. Creating an artifact from a `path` reads the
file with the OpenCode process's own access, outside OpenCode's file
permissions, and any source token can then read it.

`spool-server artifact purge <id>` deletes an artifact; purging a session also
deletes the artifacts it created.
