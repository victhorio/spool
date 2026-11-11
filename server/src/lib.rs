mod access;
pub mod admin;
mod artifacts;
mod auth;
mod browse;
mod delivery;
mod device;
mod error;
mod http;
mod page;
mod reference;
mod request;
mod retrieval;
mod snapshot;
mod sync;

use std::time::Duration;

use anyhow::Context;
use sqlx::{
    PgPool, Postgres, Transaction,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub use access::Access;
pub use http::{Reader, router};

use error::ApiError;

/// The most an agent tool result may hold, in bytes; the plugin refuses more.
const PAGE_BYTES: usize = 24 * 1024;

pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    connect_pool(database_url, false).await
}

/// A small pool whose transactions default to read-only, for archive reads.
pub async fn connect_read(database_url: &str) -> anyhow::Result<PgPool> {
    connect_pool(database_url, true).await
}

async fn connect_pool(database_url: &str, read_only: bool) -> anyhow::Result<PgPool> {
    let mut options = database_url.parse::<PgConnectOptions>()?.options([
        ("statement_timeout", "10s"),
        ("lock_timeout", "5s"),
        ("idle_in_transaction_session_timeout", "15s"),
    ]);
    if read_only {
        options = options.options([("default_transaction_read_only", "on")]);
    }
    PgPoolOptions::new()
        .max_connections(if read_only { 2 } else { 5 })
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .context("could not connect to PostgreSQL")
}

/// A read-only transaction over one snapshot, so a page and its session agree.
async fn consistent_read(pool: &PgPool) -> Result<Transaction<'_, Postgres>, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(ApiError::database("read.begin"))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database("read.snapshot"))?;
    Ok(tx)
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!().run(pool).await.context("migration failed")
}
