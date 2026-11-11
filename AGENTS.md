# Spool

Spool archives coding-agent sessions across machines, starting with OpenCode.
It captures sessions and provides a browser and agent search/read tools, holds
artifacts (documents agents publish for people to read), and delivers messages
from the owner and agents to sessions on machines that are online.
Built-in reader-agent orchestration and session resume remain out of scope.

## Architecture

`server/` is the Rust service. It owns authentication, validation and durable
storage in PostgreSQL, with migrations in `server/migrations/`. `plugin/` is the
TypeScript OpenCode plugin. It owns capture, repository discovery, OpenCode text
projections, uploads, and tool and skill registration. `web/` is the reader.
Keep OpenCode-specific parsing in the plugin and reader, not the database.
OpenCode's public exports, not its private SQLite schema, define what Spool
archives. SDK dependencies are pinned for reproducible builds, not to restrict
the host's OpenCode version.

One public service hosts everything: `/v1/*` is for machines with source tokens,
`/healthz` needs no credentials, and every other route is the reader behind
Cloudflare Access, which the server verifies itself. The backend must not depend
on a user's computer staying online. Railway is the initial host, not a reason
to build a provider abstraction: keep the service ordinary Rust and Postgres.

Pushing to `main` deploys production. Each machine keeps running the plugin
commit it installed until `bun run install-plugin` runs there again, so server
changes that plugins depend on must deploy first.

Efficiency is part of the project's appeal: avoid unnecessary work, copies and
allocations, and use measured workloads to guide more involved optimizations.

Use synthetic sessions for fixtures. Never commit real conversations or
credentials or use installed agent state for tests. Validation must not call live
models. Do not add a README until preparing the project for public release.

## Docs

- [Ingestion](docs/ingestion.md): sources and tokens, the snapshot consistency
  model, and capture.
- [Retrieval](docs/retrieval.md): agent search tools, repository identity and
  session references.
- [Browser](docs/browser.md): the reader and its access boundary.
- [Artifacts](docs/artifacts.md): documents agents publish for people to read.
- [Messages](docs/messages.md): messaging sessions on online machines.
- [Development](docs/development.md): the isolated local stack and validation.
- [Deployment](docs/deployment.md): production on Railway and Cloudflare.
- [Client setup](docs/client-setup.md): installing the plugin on a machine.

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

Plain `cargo test` skips the PostgreSQL tests; `bun run validate` runs them.
