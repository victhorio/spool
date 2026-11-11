# Spool

Spool archives coding-agent sessions across machines, starting with OpenCode.
It captures sessions and provides a browser and agent search/read tools, holds
artifacts (documents agents publish for people to read), and delivers messages
from the owner and agents to sessions on machines that are online.
Built-in reader-agent orchestration and session resume remain out of scope.

## Architecture

Railway is the initial target for the Rust service and PostgreSQL. The backend
must not depend on a user's computer staying online.

`server/` owns authentication, validation, and durable storage, with SQL migrations
in `server/migrations/`. `plugin/` contains the TypeScript OpenCode plugin,
which owns capture, repository discovery, OpenCode text projections, uploads and
tool/skill registration. Keep OpenCode-specific parsing in the plugin and `web/`
reader rather than the database. One public service hosts everything: `/v1/*`
is for machines with source tokens, `/healthz` needs no credentials, and every
other route is the web reader behind Cloudflare Access, which the server
verifies itself.

[Client setup](docs/client-setup.md) covers capture and retrieval on each machine.
[Ingestion](docs/ingestion.md) describes the consistency and wire contract.
[Browser](docs/browser.md) covers the reader's access boundary and API.
[Retrieval](docs/retrieval.md) covers repository identity and agent tools.
[Artifacts](docs/artifacts.md) covers documents agents publish for people to read.
[Messages](docs/messages.md) covers messaging sessions on online machines.
[Development](docs/development.md) covers the isolated local stack in `dev/`.
OpenCode's public exports, not its private SQLite schema, define what Spool
archives. SDK dependencies are pinned for reproducible builds, not to restrict
the host's release version.

Efficiency is part of the project's appeal: avoid unnecessary work, copies, and
allocations, and use measured workloads to guide more involved optimizations.
Keep the service ordinary Rust and Postgres; Railway is the initial host, not a
reason to build a provider abstraction.

Use synthetic sessions for fixtures. Never commit real conversations or
credentials or use installed agent state for tests. Validation must not call live
models. Do not add a README until preparing the project for public release.

## Checks

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
bun install --frozen-lockfile
bun run typecheck
bun test plugin scripts
bun run validate
bun run retrieval:validate
bun run web:validate
```

`bun run validate` includes Rust HTTP/CLI tests against real isolated PostgreSQL.
Plain `cargo test` skips those tests without `SPOOL_TEST_DATABASE_URL`.
`bun run web:validate` builds and exercises the reader in headless Chrome against
a synthetic archive, not the user's preview database.
