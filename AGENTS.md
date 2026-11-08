# Spool

Spool archives coding-agent sessions across machines, starting with OpenCode.
The first milestone is reliable capture, upload, and storage. Search tools,
delegated readers, and session resume are out of scope for now.

## Architecture

Railway hosts the Rust ingestion service and PostgreSQL. The backend must not
depend on a user's computer staying online. Tailnet access is optional;
authentication and the ingestion contract are not designed yet.

`server/` owns authentication, validation, and durable storage, with SQL migrations
in `server/migrations/`. `plugin/` will contain the TypeScript OpenCode plugin,
which owns capture, backfill, and retrying uploads. Keep OpenCode-specific parsing
in the plugin rather than the database. The two components share an ingestion
contract and live together so they can evolve together.

Efficiency is part of the project's appeal: avoid unnecessary work, copies, and
allocations, and use measured workloads to guide more involved optimizations.
Keep the service ordinary Rust and Postgres; Railway is the initial host, not a
reason to build a provider abstraction.

Use synthetic sessions for fixtures. Never commit real conversations or
credentials. Do not add a README until preparing the project for public release.

## Checks

Run from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
