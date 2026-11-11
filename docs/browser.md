# Session browser

The reader lists sessions recent-first, with a source filter and search over
titles, directories and source labels. A conversation opens at
`/sessions/{reference}`, using the same [session
reference](retrieval.md#session-references) as the agent tools, at its latest
messages; `#12` opens message 12, numbered as the tools number messages.
Reasoning, tools, system context and compaction summaries are collapsible.
Archived HTML is inert, remote images aren't loaded, and Markdown is sanitized.
The reader also lists [artifacts](artifacts.md), approves [device
logins](ingestion.md#device-login), and [messages](messages.md) sessions whose
machine is online. Those last two are its only writes.

## Access

Cloudflare Access protects every route except `/v1/*`; see
[deployment](deployment.md). Railway's origin can be reached without passing
through Cloudflare, so the server verifies the Access assertion itself on every
reader request, against the team's signing keys, issuer and the application's
AUD tag. `serve` enables the reader only when both `ACCESS_ISSUER` and
`ACCESS_AUD` are set, and fails to start if only one is.

Archive reads use a separate two-connection pool whose transactions default to
read-only. That prevents accidental writes; it is not a privilege boundary.
The reader API also rejects cross-site requests, and when an Access session
expires, the page reloads to follow Cloudflare's login redirect.

Session metadata stays opaque JSON: PostgreSQL's JSON operators reject escaped
NUL, which archived documents may contain, so the server interprets metadata in
Rust and the session list scans it per request rather than using an index.
