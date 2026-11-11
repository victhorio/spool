# Local development

Requires Docker with Compose, Rust, Bun and OpenCode on PATH. The harness runs
the installed OpenCode; compatibility rests on the public plugin, service,
session export and event APIs that validation exercises.

## Sandbox

`bun run local:up` starts PostgreSQL and the release service image in containers,
provisions a source, and runs real OpenCode with the local plugin. It prints the
endpoints and state directory; Ctrl-C or `bun run local:down` stops it and keeps
its database volume. Restart it after changing plugin code: like the validation
scripts, it packs the plugin and loads the unpacked package, so it sees only the
package's `files`, as an installation does.

Everything OpenCode would read or write, including HOME, XDG paths, config and
checkpoints, lives under `.local/dev/`; it gets no model keys, personal config or
credentials, and model downloads and updates are disabled. `dev/fixtures.ts`
seeds synthetic conversations through the import API without model calls.
`dev/stack.ts` lets scripts provision more sources, run operator commands and
query the isolated database.

Compose caps the database at 1 GiB: concurrent 64 MiB projected uploads
exhausted an earlier 256 MiB cap, so wire limits are not a memory budget.

A local copy of the production archive is useful for previewing the reader.
Treat it as private data, not a fixture, and never expose its PostgreSQL beyond
loopback.

## Validation

`bun run validate` builds a fresh sandbox and drives the real plugin, HTTP,
release container and PostgreSQL through capture scenarios such as undo,
conflicts, lost acknowledgements, outages and purge, using a local fault proxy
rather than mocks. It also runs the Rust tests against that database. Only its
missed-event fixture writes OpenCode's private SQLite directly, to simulate an
edit without events. Failed or interrupted runs keep logs under the printed
`.local/` path.

The Rust PostgreSQL tests run only when `SPOOL_TEST_DATABASE_URL` points at a
disposable PostgreSQL superuser; each test creates and drops its own database.

`bun run retrieval:validate` checks repository capture with real Git checkouts,
worktrees and clones, and runs the actual General agent loop through skill
loading and the retrieval tools with a scripted model. It verifies schemas,
permissions and evidence delivery, not a model's choices.

`bun run web:validate` builds the reader and runs Playwright in installed Google
Chrome, with a disposable profile, against a synthetic archive and a synthetic
Access team (`dev/access.ts`). Screenshots and traces go to `.local/web-artifacts/`.
