# Local development

Requires Docker with Compose, Rust, Bun and OpenCode on PATH. Install JavaScript
dependencies with `bun install --frozen-lockfile`. The harness runs the installed
OpenCode and reports its version; SDK dependency pins do not restrict the host's
release version. Compatibility depends on the public plugin, service, session
list/export and event APIs exercised by validation.

## Persistent sandbox

Run `bun run local:up`. It builds the release service image, starts PostgreSQL 17
and the service in containers, provisions a source, and starts real OpenCode
with the local plugin. Ports are dynamically assigned and bound only to
loopback. The command prints the endpoints and state directory. Ctrl-C stops
the processes and containers, retaining the sandbox's database volume.
`bun run local:down` also stops that sandbox from another terminal.
Interrupting startup waits for in-flight container creation before cleaning up.

OpenCode's authenticated registration, config, cache, data, HOME, XDG paths,
temporary directory and plugin checkpoints all live under `.local/dev/`.
The server inherits only PATH, not model keys, personal configs or credentials,
and runs with an isolated working directory.
Model downloads, updates and project config are disabled. The synthetic helpers
in `dev/fixtures.ts` seed structured conversations through the import API without
model calls. Do not use this sandbox to test against real conversations.

Compose caps database memory at 1 GiB and service memory at 512 MiB. Concurrent
64 MiB projected uploads exhausted the former 256 MiB database cap; wire limits
are not a database-memory budget. OpenCode runs natively to exercise the
installed runtime without another heavy image.
The `dev/stack.ts` LocalStack API lets a script provision another source/store,
execute operator commands or inspect isolated PostgreSQL. Each root gets its
own Compose project and volume. The fixed local-only DB password in Compose is
not a deployment credential.

## Repeatable validation

Run `bun run validate`. It creates a fresh sandbox separate from `.local/dev`,
then exercises the real plugin → HTTP → release container → PostgreSQL path:

- Structured ordered exports with tools, reasoning, an inline attachment and NUL.
- Metadata-only deltas with unchanged message-row versions.
- Explicit `/spool-repair` for eventless edits with unchanged timestamps/counts;
  unchanged audits do not upload confirmations or advance revisions.
- A hot session cannot starve other captures; there are no periodic full sweeps.
- One collector across Locations, restart checkpoints/backfill and independent stores.
- Staged and committed undo, including removal of the committed tail.
- A genuine competing acceptance/409, with source mutation before recapture.
- Lost acknowledgement after commit and service-container outage/recovery.
- Local deletion retention and purge without resurrection or cross-source damage.
- Rust HTTP/CLI regressions against real PostgreSQL, including upload/purge races, rollback,
  raw JSON fidelity, credential rotation/revocation and request/admission limits.
- Forced generic plans with 100,000-message confirmations, reversal and truncation;
  collection-count limits, object shapes, and exact 16 MiB archive / 64 MiB
  projected-envelope boundaries.

The harness checks startup reconciliation skips unchanged sessions but captures
changed metadata and missing sessions. It uses a local HTTP fault proxy;
no service dependencies or OpenCode exports are mocked.
Only the missed-event fixture writes private SQLite directly, within its isolated
test store; production capture uses the public export API. There are no live
model calls. Containers/volumes are removed on completion. Successful runs
remove their temporary roots; failures and SIGINT/SIGTERM interruptions retain
logs/state at the printed path after cleaning up processes and containers/volumes.

`cargo test --workspace` alone skips PostgreSQL regressions unless
`SPOOL_TEST_DATABASE_URL` is set. Supply a disposable PostgreSQL superuser.
Tests create/drop a unique disposable database rather than clearing an existing
one. `bun run validate` supplies the local container URL automatically.

`bun run retrieval:validate` uses isolated OpenCode stores, real Git checkouts,
worktrees and clones, and Rust/PostgreSQL to exercise repository capture and the
three [retrieval tools](retrieval.md). A deterministic AI SDK fixture runs the
actual General agent loop through skill loading, find, search and read without
calling a model. It verifies runtime schemas, permissions and evidence delivery,
not whether a model chooses the right search or delegation. Explore permissions
remain unchanged. The fixture also checks giant-message continuation, stale
revisions, missing projections, removed worktrees and server outages.

## Installed clients and server hosting

Use [client setup](client-setup.md) for an actual OpenCode installation, including
configuration, reloads and retrieval checks.
The sandbox above uses isolated settings and does not configure your installed
client.

For Railway, build `server/Dockerfile` from the repository root and supply
`DATABASE_URL`. Default command serves and migrates before listening, honors
`PORT` (default 8080), and binds `BIND_ADDR` (default 0.0.0.0). `ACCESS_ISSUER`
and `ACCESS_AUD` enable the web reader; see [public deployment](public-deployment.md).
The image includes the built frontend. Use `/healthz` for readiness. The image runs as non-root. Operator CLI commands use the same
binary and database, not a public admin endpoint. Deployment, TLS, backups and
real-client installation are separate operator actions. The production ingestion
service is connected to GitHub `main`: pushing there can trigger deployment.
Push review branches when deployment is deferred; do not change the deployment
integration merely to publish code.
