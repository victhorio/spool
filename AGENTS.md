# Spool

Spool archives coding-agent sessions across machines, starting with OpenCode.
The first milestone is reliable capture, upload, and storage. Search tools,
delegated readers, and session resume are out of scope for now.

## Architecture

Railway is the initial target for the Rust service and PostgreSQL. The backend
must not depend on a user's computer staying online. Tailnet access is optional;
source-scoped bearer tokens are not.

`server/` owns authentication, validation, and durable storage, with SQL migrations
in `server/migrations/`. `plugin/` contains the TypeScript OpenCode plugin,
which owns capture, backfill, and retrying uploads. Keep OpenCode-specific parsing
in the plugin rather than the database. The two components share an ingestion
contract and live together so they can evolve together.

[Ingestion](docs/ingestion.md) describes the consistency and wire contract.
[Development](docs/development.md) covers the isolated local stack in `dev/`.
The plugin is pinned to OpenCode 2.2.5; public exports, not its private SQLite
schema, define what Spool archives.

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
bun run validate
```

`bun run validate` includes Rust HTTP/CLI tests against real isolated PostgreSQL.
Plain `cargo test` skips those tests without `SPOOL_TEST_DATABASE_URL`.
